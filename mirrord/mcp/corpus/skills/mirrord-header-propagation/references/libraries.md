# Detecting and enabling baggage propagation in existing stacks

Library support changes between versions. Treat this as where to look, not as proof: a hop counts as covered only after you've confirmed the header or attribute on a real message or in a test (see SKILL.md Step 7).

## OpenTelemetry

**Propagators.** The spec default for `OTEL_PROPAGATORS` is `tracecontext,baggage`, and the Java agent, Python, Node `NodeSDK`, and .NET SDKs follow it. Watch for:

- An explicit `OTEL_PROPAGATORS` that leaves out `baggage` (e.g. `tracecontext`, `b3`, `xray`). Add `baggage`: `tracecontext,baggage` or `xray,tracecontext,baggage`.
- **Go sets no global propagator by default.** Look for `otel.SetTextMapPropagator(...)`. Without it, `otelhttp` / `otelgrpc` propagate nothing. It needs:
  ```go
  otel.SetTextMapPropagator(propagation.NewCompositeTextMapPropagator(
      propagation.TraceContext{}, propagation.Baggage{}))
  ```
- Node code that builds its own `NodeTracerProvider` and calls `register({ propagator })` with only `W3CTraceContextPropagator`. Use a `CompositePropagator` with `W3CBaggagePropagator`.

**Instrumentation coverage by language** (verify against the versions in the lockfile):

| Language | HTTP / gRPC | Kafka | RabbitMQ | SQS | Pub/Sub |
|----------|-------------|-------|----------|-----|---------|
| Java (agent) | auto | `kafka-clients`, `spring-kafka`: auto | `amqp-client`, `spring-rabbit`: auto | AWS SDK v1/v2: set `otel.instrumentation.aws-sdk.experimental-use-propagator-for-messaging=true` so the configured propagators inject into message attributes | not auto — enable `setEnableOpenTelemetryTracing(true)` on the client library's Publisher/Subscriber builder |
| Python | `opentelemetry-instrumentation-{requests,httpx,urllib3,flask,django,fastapi,grpc}` | `-kafka-python`, `-confluent-kafka`, `-aiokafka` | `-pika`, `-aio-pika` | `-boto3sqs` injects/extracts message attributes | `google-cloud-pubsub` `PublisherOptions(enable_open_telemetry_tracing=True)` / `SubscriberOptions(...)` |
| Node | `auto-instrumentations-node` | `@opentelemetry/instrumentation-kafkajs` | `-amqplib` | `-aws-sdk` (SQS send/receive message attributes) | `@google-cloud/pubsub` `enableOpenTelemetryTracing: true` |
| Go | `otelhttp`, `otelgrpc` (manual wiring) | no official instrumentation for sarama/kafka-go/confluent-kafka-go; franz-go has `plugin/kotel`. Otherwise use a header carrier (propagation-layer.md) | none — use a header carrier | `otelaws` traces calls but doesn't inject into message attributes — use a carrier | `pubsub.ClientConfig{EnableOpenTelemetryTracing: true}` |
| .NET | ASP.NET Core / HttpClient / gRPC instrumentation | Confluent.Kafka: no official — use a carrier | RabbitMQ.Client 7+: built-in | `OpenTelemetry.Instrumentation.AWS` — check SQS injection for the version in use | check client version |
| Ruby | `opentelemetry-instrumentation-all` | `ruby-kafka`, `rdkafka` | `bunny` | `aws_sdk` — check SQS attribute injection | — |

**Google Pub/Sub attribute names.** The client libraries' built-in OTel support writes propagated fields as `googclient_`-prefixed attributes (e.g. `googclient_baggage`). Either point the mirrord `message_filter` at that name, or also set a plain `baggage` attribute in a publish wrapper. Confirm the name on a real message first.

**Consumer spans.** Several messaging instrumentations start the consumer span with a *link* to the producer span instead of a parent. Baggage should still be restored into the active context during processing; if a test shows it isn't, extract in the handler.

**Operator injection.** A pod annotated `instrumentation.opentelemetry.io/inject-<lang>` gets auto-instrumentation from the OpenTelemetry Operator; its `Instrumentation` CR sets `spec.propagators`. Check that list includes `baggage`. That CR is infra — tell the user rather than editing a live cluster.

## Spring Boot 3 (Micrometer Tracing)

Micrometer Tracing (with the OTel or Brave bridge) only forwards baggage keys it's told about.

```properties
management.tracing.propagation.type=w3c
management.tracing.baggage.remote-fields=mirrord-session
# Kafka / RabbitMQ templates and listeners only propagate with observation on:
spring.kafka.template.observation-enabled=true
spring.kafka.listener.observation-enabled=true
spring.rabbitmq.template.observation-enabled=true
spring.rabbitmq.listener.simple.observation-enabled=true
```

`remote-fields` takes the **baggage key** (`mirrord-session`), not the header name. Templates/listener factories built by hand instead of from Boot autoconfig need `setObservationEnabled(true)` in code.

Spring Boot 2 + Spring Cloud Sleuth: `spring.sleuth.propagation.type=w3c` and `spring.sleuth.baggage.remote-fields=mirrord-session`.

## Datadog (`dd-trace-*`)

Recent tracer versions support W3C baggage as a propagation style alongside `datadog` and `tracecontext`, and include it by default in newer releases. Check:

- `DD_TRACE_PROPAGATION_STYLE` / `DD_TRACE_PROPAGATION_STYLE_INJECT` / `_EXTRACT`. If set, add `baggage` (e.g. `datadog,tracecontext,baggage`). If the tracer version predates baggage support, upgrading the tracer is the fix — call that out rather than adding a parallel hand-rolled layer.
- Messaging: Datadog injects context into Kafka headers and RabbitMQ headers, but for SQS / SNS / Pub/Sub it typically uses a single `_datadog` attribute holding its own context. mirrord can't filter that as `baggage`. Inspect a real message; if there's no plain `baggage` header/attribute, add one in a producer wrapper (propagation-layer.md) while keeping Datadog in place.
- Integration toggles like `DD_TRACE_KAFKA_ENABLED=false` / `DD_KAFKA_PROPAGATION_ENABLED=false` (the latter on some tracers) disable header injection.

## Others — treat as not covering until verified

- **Sentry:** emits `baggage` carrying `sentry-*` entries for its own use; whether third-party members like `mirrord-session` are forwarded depends on the SDK. Check before relying on it.
- **New Relic, Elastic APM, AWS X-Ray SDK (non-OTel):** propagate trace context, but generally not W3C baggage members. Add OTel baggage-only propagation alongside.
- **Service mesh (Istio, Linkerd, Consul):** never propagates for the app.
- **API gateways / ingress (NGINX, Envoy, Kong, ALB):** usually pass `baggage` through unchanged. Check for header allowlists or `proxy_set_header` rules that drop unknown headers, and for CDNs that strip them.
