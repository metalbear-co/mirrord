---
title: Monitoring
date: 2024-04-01T13:37:00.000Z
lastmod: 2024-04-01T13:37:00.000Z
draft: false
images: []
linktitle: Monitoring
menu: null
docs: null
teams: null
weight: 500
toc: true
tags:
  - team
  - enterprise
description: Monitoring with mirrord for Teams
---

The mirrord Operator can produce logs in JSON format that can be digested by most popular logging solutions (DataDog, Dynatrace, etc). To enable JSON logging, set `operator.jsonLog` to `true` in the Operator Helm chart values. The log level is `INFO` by default, and can be changed by setting `operator.logLevel` in the Helm chart values, or alternatively by using the `RUST_LOG` environment variable in the Operator container, which takes values in the following format: `mirrord={log_level}` (e.g. `mirrord=debug`).

{% hint style="info" %}
This feature is available to users on the Team and Enterprise pricing plans.
{% endhint %}

### Functional Logs

The following logs are written with log level `INFO`, and can be used for dashboards within monitoring solutions in order to monitor mirrord usage within your organization:

Log messages:

* Copy Target
* Port Steal
* Port Mirror
* Port Release
* Session Start
* Session End
* Message Processing

Session, port, and copy-target lifecycle logs use the following fields. `Message Processing` uses its own fields described below.

| field             | description                                                                                                                                                                  | events                                                                         |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| target            | the session's target                                                                                                                                                         | `All lifecycle events`                                                          |
| client\_hostname  | `whoami::hostname` of client                                                                                                                                                 | `All lifecycle events`                                                          |
| client\_name      | `whoami::realname` of client                                                                                                                                                 | `All lifecycle events`                                                          |
| client\_user      | Kubernetes user of client (via k8s RBAC)                                                                                                                                     | `All lifecycle events`                                                          |
| client\_id        | unique client id produced from client's certificate                                                                                                                          | `All lifecycle events`                                                          |
| client\_cli\_version | version of the client's mirrord CLI                                                                                                                                          | `All lifecycle events`                                                          |
| session\_id       | unique id for individual mirrord sessions                                                                                                                                    | `Port Steal` `Port Mirror` `Port Release` `Session Start` `Session End` |
| session\_duration | the session's duration in seconds                                                                                                                                            | `Session End`                                                                |
| port              | port number                                                                                                                                                                  | `Port Steal` `Port Mirror` `Port Release`                                  |
| http\_filter      | the client's configured [HTTP Filter](https://metalbear.com/mirrord/docs/config#feature.network) | `Port Steal`                                                                  |
| scale\_down       | whether the session's target was scaled down                                                                                                                                 | `Copy Target`                                                                |

#### Message Processing

{% hint style="info" %}
Message processing functional logs require mirrord Operator `3.188.0` or later.
{% endhint %}

The Operator emits a `Message Processing` log when mirrord handles an HTTP request or a message from a supported queue or message bus. These logs connect the mirrord session key with application correlation and tracing metadata, making it possible to investigate where an asynchronous flow stopped propagating trace context.

The logs are emitted automatically through the Operator's normal logging pipeline at level `INFO`. Set `operator.jsonLog` to `true` when your collector needs structured fields. In JSON output, the message processing values are nested under the top-level `fields` object, as shown in the abbreviated examples below. The same records can also be sent through the Operator's [OpenTelemetry log exporter](#exporting-logs) when its configured log level includes `INFO`.

All message processing records contain the following fields:

| field | description |
| --- | --- |
| `service_name` | Name of the intercepted target workload |
| `session_key` | Correlation key of the mirrord session that handled the event |
| `event_type` | Kind of event: `http`, `queue`, or `message_bus` |
| `mode` | Routing mode: `steal` or `mirror` |
| `event_timestamp` | Time at which the Operator produced the event |

##### HTTP

HTTP requests and responses are logged as two separate lifecycle records. The request record contains the intercepted request context:

| field | description |
| --- | --- |
| `method` | HTTP request method |
| `path` | Request URI path |
| `request_headers` | Request header map, serialized as a JSON string. Values of headers that often hold secrets are redacted, see the note below. Not present when `operator.hideHeadersAndProperties` is `true` |
| `correlation_id` | Value of a recognized correlation ID header, when present |
| `traceparent` | W3C Trace Context `traceparent` header, when present |
| `tracestate` | W3C Trace Context `tracestate` header, when present |
| `baggage` | W3C baggage header, when present |

For example:

```json
{
  "timestamp": "2026-08-17T18:15:08.569771Z",
  "level": "INFO",
  "fields": {
    "message": "Message Processing",
    "service_name": "checkout",
    "session_key": "test-123",
    "event_type": "http",
    "mode": "steal",
    "event_timestamp": "2026-08-17 18:15:08.569354652 UTC",
    "method": "POST",
    "path": "/orders",
    "request_headers": "{\"baggage\":[\"mirrord-session=test-123\"],\"traceparent\":[\"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01\"],\"x-correlation-id\":[\"order-123\"]}",
    "correlation_id": "order-123",
    "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    "baggage": "mirrord-session=test-123"
  },
  "target": "operator_context::event::functional_log"
}
```

When a stolen request completes, the Operator emits a separate response record containing `response_status_code`:

```json
{
  "timestamp": "2026-08-17T18:15:08.583869Z",
  "level": "INFO",
  "fields": {
    "message": "Message Processing",
    "service_name": "checkout",
    "session_key": "test-123",
    "event_type": "http",
    "mode": "steal",
    "event_timestamp": "2026-08-17 18:15:08.583733458 UTC",
    "response_status_code": 202
  },
  "target": "operator_context::event::functional_log"
}
```

The response record does not currently contain a request identifier. When several requests are handled concurrently by one session, consumers should not assume that request and response records can be paired using their timestamps alone.

##### Queues and message buses

Queue and message bus records use the following fields when the broker provides the corresponding metadata:

| field | description |
| --- | --- |
| `queue_type` | Broker type: `sqs`, `azure_service_bus`, `bullmq`, `rmq`, `gcp_pubsub`, `kafka`, or `redis_pubsub` |
| `queue_name` | Queue, topic, subscription, or Redis channel name |
| `message_id` | Broker-provided message or job identifier |
| `correlation_id` | Broker-provided correlation ID or a recognized correlation ID property/header |
| `message_properties` | Message attributes, properties, or Kafka headers, serialized as a JSON string. Values of properties that often hold secrets are redacted, see the note below. Not present when `operator.hideHeadersAndProperties` is `true` |
| `traceparent` | W3C Trace Context value extracted from message properties or headers |
| `tracestate` | W3C Trace Context value extracted from message properties or headers |
| `baggage` | W3C baggage value extracted from message properties or headers |
| `partition` | Kafka partition |
| `offset` | Kafka offset |
| `message_key` | Kafka message key, when present |
| `payload_size` | Redis Pub/Sub payload size in bytes; the payload itself is not logged |

For example, a queue message can produce:

```json
{
  "timestamp": "2026-08-17T18:16:12.123456Z",
  "level": "INFO",
  "fields": {
    "message": "Message Processing",
    "service_name": "orders",
    "session_key": "test-123",
    "event_type": "queue",
    "mode": "steal",
    "event_timestamp": "2026-08-17 18:16:12.123000000 UTC",
    "queue_type": "sqs",
    "queue_name": "orders",
    "message_id": "9f2c...",
    "correlation_id": "order-123",
    "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    "baggage": "mirrord-session=test-123",
    "message_properties": "{\"CorrelationId\":\"order-123\",\"baggage\":\"mirrord-session=test-123\",\"traceparent\":\"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01\"}"
  },
  "target": "operator_context::event::functional_log"
}
```

Starting with mirrord Operator `3.215.0`, the Operator writes `[REDACTED]` in place of the value of a header or message property when its name often holds a secret. The Operator uses only the ASCII letters and digits of the name, in lowercase, and the name matches when they contain one of these parts: `accesskey`, `apikey`, `appcheck`, `auth`, `cookie`, `credential`, `csrf`, `encryptioncustomerkey`, `encryptionkey`, `functionskey`, `hmac`, `jwt`, `oidcdata`, `passphrase`, `passw`, `privatekey`, `pwd`, `secret`, `session`, `signature`, `subscriptionkey`, `token`, or `xsrf`. For example, the values of `Authorization`, `Cookie`, `X-Api-Key`, `api.key`, `Client-Secret` and `db_password` are redacted. The name stays in the record, so you can still see which headers or properties a message had.

{% hint style="warning" %}
HTTP headers and message properties can contain credentials, personal information, or other sensitive values. The Operator only redacts the values of names that contain one of the parts above. It examines only the name, not the value. So it logs the values of all other names as they are, and it also logs a secret inside a value, such as a nested field of a BullMQ job property. HTTP bodies and raw broker payloads are not logged, but message properties can still contain application data, such as the top-level fields of a BullMQ job's `data` payload. Access controls, retention policies, and collector-side redaction should account for the metadata included in these records.

To hide the header and property maps from these records, set `operator.hideHeadersAndProperties` to `true` in the Operator Helm chart values. The records then do not have the `request_headers` and `message_properties` fields, but they still have the `correlation_id`, `traceparent`, `tracestate` and `baggage` fields.
{% endhint %}

##### Querying the logs

You can inspect message processing records directly in the Operator's Kubernetes logs. The following commands assume that the Operator runs in the `mirrord` namespace.

Stream message processing records as they are emitted:

```bash
kubectl logs --namespace mirrord deployment/mirrord-operator --follow --tail=5 \
  | grep --line-buffered '"Message Processing"'
```

You can set `operator.jsonLog` to `true` (default setting) before using the `jq` examples so that each log line is valid JSON.
Pipe the records through `jq` to make the JSON easier to read:

```bash
kubectl logs --namespace mirrord deployment/mirrord-operator --follow --tail=5 \
  | grep --line-buffered '"Message Processing"' \
  | jq .
```

To follow records for one mirrord session, replace `<your-session-key>` with its session key:

```bash
kubectl logs --namespace mirrord deployment/mirrord-operator --follow --tail=5 \
  | jq --compact-output 'select(.fields.message == "Message Processing" and .fields.session_key == "<your-session-key>")'
```

{% hint style="info" %}
When `grep` writes to another command, it buffers its output by default. On a live stream, this can make the command appear to hang even when matching records are available. `--line-buffered` flushes each matching record immediately.
{% endhint %}

### Prometheus

The mirrord Operator can expose Prometheus metrics if enabled (the default endpoint is `:9000/metrics`).

#### Helm

```yaml
# values.yaml for mirrord-operator helm chart
operator:
  ...
  metrics: true
  ...
```

#### Manual

| env                        | description              | type              | default        |
| -------------------------- | ------------------------ | ----------------- | -------------- |
| OPERATOR\_METRICS\_ENABLED | enable metrics endpoint  | "true" \| "false" | "false"        |
| OPERATOR\_METRICS\_ADDR    | metrics http server addr | SocketAddr        | "[::]:9000"    |

#### Exposed metrics

| metric                           | description                                          | labels                                                  | minimum version                  |
| -------------------------------- | ---------------------------------------------------- | ------------------------------------------------------- | ---------------------------------------------------- | 
| mirrord\_license\_valid\_seconds | Seconds until license expiration            |                                                         | operator 3.101.0 (helm chart 1.15.0)|                            
| mirrord\_sessions\_create\_total | Count of created sessions                            | `client_hostname` `client_name` `client_user` `user_id` | operator 3.101.0 (helm chart 1.15.0) |
| mirrord\_sessions\_duration      | Histogram for finished sessions duration | `client_hostname` `client_name` `client_user` `user_id` | operator 3.101.0 (helm chart 1.15.0) | 
| mirrord_ci_sessions_create_total | Count of created CI sessions                            | `client_hostname` `client_name` `client_user` `user_id` | operator 3.163.0 |
| mirrord_ci_sessions_duration  | Histogram for finished CI sessions duration | `client_hostname` `client_name` `client_user` `user_id` | operator 3.163.0 | 
| mirrord\_operator\_ping\_latency | Histogram for round trip latency between the mirrord users and the Operator, helps identify infrastructure issues that may affect mirrord performance | `client_hostname` `client_name` `client_user` `user_id`                        | operator 3.122.0 (helm chart 1.35.0) |
| mirrord\_stolen\_connections\_count     | Count of stolen TCP connections | `port` `namespace` `target` `user_id` | operator 3.122.0 (helm chart 1.35.0) |
| mirrord\_stolen\_requests\_count      | Count of stolen HTTP requests | `port` `namespace` `target` `user_id`| operator 3.122.0 (helm chart 1.35.0) |
| mirrord\_read\_sqs\_messages\_count | Count of SQS messages read from `original_queue`  | `original_queue`                                        | operator 3.125.0 (helm chart 1.38.0) |
| mirrord\_sqs\_messages\_forwarded\_to\_user\_count | Count of SQS messages read from `original_queue`, forwarded to the local service of `k8s_user`, `local_username`.  | `k8s_user`, `local_username`, `original_queue` | operator 3.125.0 (helm chart 1.38.0) |
| mirrord\_unmatched\_sqs\_messages\_count | Count of SQS messages read from `original_queue` that weren't matched by any user's filter and were sent to the main output queue for the deployed application. | `original_queue` | operator 3.125.0 (helm chart 1.38.0) |
| mirrord_previews_create_total | Count of created preview sessions | `target_namespace` `target_kind` `target_name` | operator 3.163.0 |
| mirrord_previews_duration  | Histogram for finished preview sessions duration | `target_namespace` `target_kind` `target_name` | operator 3.163.0 | 

### OpenTelemetry

{% hint style="info" %}
The features under the "OpenTelemetry" heading require at least operator chart version 1.46.0.
{% endhint %}

OTEL logs and traces can be sent from the operator to a configured OTLP collector endpoint using an HTTP exporter.

{% hint style="info" %}
As of version `3.186.0`, both `operator.otelLogExportUrl` and `operator.otelTraceExportUrl` may reference environment variables set in operator.extraEnv using $(VAR_NAME) syntax.
{% endhint %}

#### Exporting Logs

To export logs from the operator to an endpoint, set `operator.otelLogExportUrl` to the URL in the Operator Helm chart values. You _must_ set this value to export logs. This value does not affect the logs which are printed by the operator to `stdout` and are always enabled.

The log level is `INFO` by default, and can be changed by setting `operator.otelLogLevel` in the Helm chart values (or alternatively by using the `OTEL_RUST_LOG` environment variable in the Operator container), which takes values in the following format: `mirrord={log_level}` (e.g. `mirrord=debug`).

Note that this log level is separate to that defined for logs controlled by `operator.logLevel`, which are printed by the operator to `stdout`.

#### Exporting Traces

To export traces from the operator to an endpoint, set `operator.otelTraceExportUrl` to the URL in the Operator Helm chart values. You _must_ set this value to export traces.

#### Using the downward API

As of version `3.186.0`, entries in `operator.extraEnv` can take a full environment variable spec (`valueFrom`) instead of a plain string value. Any valid environment variable source works, not just the downward API: `valueFrom` with `secretKeyRef` or `configMapKeyRef` renders the same way.
This means you can reference [Kubernetes downward API](https://kubernetes.io/docs/concepts/workloads/pods/downward-api/) values in `operator.otel*ExportUrl` variables, for example:

```yaml
# values.yaml
operator:
  extraEnv:
    HOST_IP:
      valueFrom:
        fieldRef:
          fieldPath: status.hostIP
    OTLP_PORT: "4318"
  otelLogExportUrl: "http://$(HOST_IP):$(OTLP_PORT)/v1/logs"
```

#### Context Propagation

{% hint style="info" %}
This feature requires at least mirrord version 3.184.0.
{% endhint %}

You can use the `mirrord.json` file to propagate `traceparent` and `baggage` values to the Operator when running mirrord:

```json
{
  "traceparent": "<trace ID>",
  "baggage": "my_key=my_value,another_key=a_second_value"
}
```

_Note that it is expected that the trace ID value of `traceparent` is not hardcoded, but rather handled and passed in by some wrapper around mirrord. This is because duplicate trace IDs will lead to strange behaviour._

The Operator will propagate these values into exported spans for some frequently used actions, including creating a new resource. Other actions, especially those that result from Kubernetes resource reconciliation, will propagate these values in future versions.

For more info about using `traceparent` and `baggage`, see [the OpenTelemetry docs about context propagation](https://opentelemetry.io/docs/concepts/context-propagation/).

### Pre-Built Dashboards

#### DataDog Dashboard

We offer a DataDog dashboard you can import to track statistics.

Download it [here](https://github.com/metalbear-co/docs/tree/main/docs/managing-mirrord/assets/Mirrord_datadog_Operator_Dashboard.json).

#### Grafana Dashboard

Alternatively there is a Grafana dashboard you can import to track statistics.

Download it [here](https://github.com/metalbear-co/docs/tree/main/docs/managing-mirrord/assets/Mirrord_grafana_Operator_Dashboard.json).

### fluentd

If you are using fluentd you can add a filter to unpack some values from the "log" message:

```
<filter kubernetes.var.log.containers.**_mirrord_mirrord-operator-**>
  @type parser
  key_name log
  reserve_data true
  remove_key_name_field true
  <parse>
    @type json
  </parse>
</filter>
```

This will expand all the extra fields stored in the "log" field.

#### fluentd + Elasticsearch

Assuming you are using `logstash_format true` and the connected mapping will store the extra fields in a `keyword` type, we have a ready made dashboard you can simply import.

Download it [here](https://github.com/metalbear-co/docs/tree/main/docs/managing-mirrord/assets/operator-fluentd-kibana.ndjson) (use Saved Objects to import).
