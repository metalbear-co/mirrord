---
title: "Subscribing to Events"
tags:
  - alpha
  - team
  - enterprise

description: "Stream a session's interception events as JSON with mirrord subscribe"
---
`mirrord subscribe` lets you stream live events from an active mirrord session — HTTP requests, queue messages, and operator notifications as JSON to stdout. It's useful in CI/tests: assert that a request actually got stolen to your local process instead of silently hitting the real service, so a broken interception fails the build instead of
going green.

{% hint style="info" %}
For centrally collected records that do not require a running subscriber, see [message processing functional logs](../managing-mirrord/monitoring.md#message-processing). Functional logs use the Operator's normal logging pipeline, while `mirrord subscribe` provides a live event stream filtered by session key.
{% endhint %}

**When do you receive events?**

* HTTP - you receive an event for each request/response that was routed to or from a target workload by mirrord and matches your subscribed key.
* Queue - while queue splitting is active, you receive an event for each message that matches your subscribed key. If no splitting session is active, no events are received.

Queue events support Amazon SQS, Azure Service Bus, GCP Pub/Sub, RabbitMQ, BullMQ, Kafka, NATS, NATS Pub/Sub, and Redis Pub/Sub.

In both cases: no active mirrord session = no events.

Pass `--unmatched` to also receive the queue messages your filter did *not* take, so you can see what your split is missing. See [Unmatched messages](#unmatched-messages).

{% hint style="info" %}
HTTP events come from redirected requests/responses. Queue events require [queue splitting](../sharing-the-cluster/queue-splitting.md) configured for the session, and every supported broker emits them, with these exceptions:

* **Temporal** is not currently supported.
* **Kafka Streams** consumers are not currently supported (standard Kafka consumers are).

Need support for more events? [Open a GitHub issue](https://github.com/metalbear-co/mirrord/issues) or reach out in the [mirrord Slack community](https://metalbearcommunity.slack.com/ssb/redirect)
{% endhint %}

### Prerequisites

* A running session started with a known key: `mirrord exec --key <KEY> ...`. If you don't pass `--key`, a random key is generated for the session (`mirrord up` is the exception, it defaults to your OS username).

### Usage

In one terminal, run the session you want to observe:

```sh
mirrord exec --key my-key -t deployment/my-app -- <cmd>
```

In another, subscribe to its events:

```sh
mirrord subscribe --key my-key
```

The key can also come from the `key` field in your mirrord config (e.g. `mirrord subscribe -f mirrord.json`)

Events stream to stdout, one compact JSON object per line, as the operator intercepts traffic for that key. Status messages go to stderr, so you can pipe events straight into `jq`:

```sh
mirrord subscribe --key my-key | jq 'select(.data.http_request)'
```

You may also pass `--pretty` to pretty-print each event.

### Options

All flags are off by default.

| Flag | Effect |
| --- | --- |
| `--session-key-field` | Adds a `session_key` field naming the session each event belongs to. |
| `--unmatched` | Also delivers queue messages that matched no session's filter. |

These flags need operator `3.210.0` or newer to take effect.

### Unmatched messages

By default you receive a queue message only when your filter claimed it. With `--unmatched` you also receive the ones nobody claimed.

```sh
mirrord subscribe --key my-key --unmatched | jq 'select(.data.queue_message.mode == "filtered")'
```

{% hint style="info" %}
This covers queue messages only. An HTTP request that matches no filter never reaches the operator.
{% endhint %}

### Events

Every event has the same envelope — `service_name` (the intercepted workload), `timestamp`, and a `data` payload:

```json
{
  "service_name": "my-app",
  "timestamp": "2026-06-17T12:00:00Z",
  "data": {
    "http_request": {
      "method": "GET",
      "uri": "/health",
      "headers": {
        "host": "my-app"
      },
      "version": "HTTP/1.1"
    }
  }
}
```

(examples are pretty-printed for clarity; actual output is compact unless `--pretty` is passed)

To work with just the payload, extract `.data` with `jq`:

```sh
mirrord subscribe --key my-key | jq '.data'
```

Every payload that describes an intercepted message carries `mode`: `steal` if only your session received it, `mirror` if the workload received a copy too, or `filtered` if it matched no filter (see [Unmatched messages](#unmatched-messages)). `http_response` has none - a response is routed with the request it answers - and neither does `lagged`, which is about the stream rather than a message.

**`data` is one of the following (payloads shown on their own):**

* **`http_request`** — an intercepted (stolen) request:

```json
{
  "http_request": {
    "mode": "steal",
    "connection_id": 4,
    "request_id": 1,
    "method": "GET",
    "uri": "/health",
    "headers": {
      "host": "my-app"
    },
    "version": "HTTP/1.1"
  }
}
```

* **`http_response`** — the response to a stolen request:

```json
{
  "http_response": {
    "connection_id": 4,
    "request_id": 1,
    "status": 200,
    "version": "HTTP/1.1",
    "headers": {
      "content-type": "application/json"
    }
  }
}
```

A request and its response are separate events. Pair them on `connection_id` **and** `request_id` together: `request_id` is only unique within its connection.

* **`queue_message`** — a queue message routed to your session.

`queue_type` is one of `sqs`, `azure_service_bus`, `gcppubsub`, `rmq`, `bullmq`, `nats`, or `natspubsub`. `message_id` and `correlation_id` are included only when the broker provides them (SQS, GCP Pub/Sub and BullMQ have no `correlation_id`). `properties` is the message's attribute bag, with values stringified: text as-is, and **binary values base64-encoded**.

An Azure Service Bus message (its application properties become `properties`):

```json
{
  "queue_message": {
    "queue_type": "azure_service_bus",
    "queue_name": "orders",
    "correlation_id": "trace-123",
    "properties": {
      "tenant": "test"
    }
  }
}
```

An SQS message, where the `payload` message attribute was binary (its bytes come back base64-encoded):

```json
{
  "queue_message": {
    "queue_type": "sqs",
    "queue_name": "orders",
    "message_id": "9f2c...",
    "properties": {
      "tenant": "test",
	  "payload": "c29tZSBlcGljIHBheWxvYWQ="
    }
  }
}
```

A GCP Pub/Sub message. `queue_name` is the subscription being split, and `properties` are the message's attributes:

```json
{
  "queue_message": {
    "queue_type": "gcppubsub",
    "queue_name": "orders-sub",
    "message_id": "11041...",
    "properties": {
      "tenant": "test"
    }
  }
}
```

A RabbitMQ message. `queue_name` is the queue the message was consumed from, and `properties` are the AMQP headers:

```json
{
  "queue_message": {
    "queue_type": "rmq",
    "queue_name": "orders",
    "correlation_id": "trace-123",
    "properties": {
      "tenant": "test"
    }
  }
}
```

A BullMQ job. `message_id` is the job ID, and `properties` are the top-level fields of the job's `data` payload — the same fields BullMQ splits filter on:

```json
{
  "queue_message": {
    "queue_type": "bullmq",
    "queue_name": "orders",
    "message_id": "42",
    "properties": {
      "tenant": "test",
      "body": "hello"
    }
  }
}
```

* **`kafka_message`** — a Kafka message stolen to your session. Kafka uses a **different schema** than `queue_message`: `topic`, `partition`, `offset`, an optional `key`, and `headers` (values stringified — text as-is, binary base64). There is no `queue_type` or `correlation_id`.

```json
{
  "kafka_message": {
    "topic": "orders",
    "partition": 0,
    "offset": 42,
    "key": "order-7",
    "headers": {
      "tenant": "test"
    }
  }
}
```

* **`redis_message`** — a Redis Pub/Sub message relayed to your session. Redis also uses a **different schema**: just the `channel` the message was published to and its `payload_size` in bytes. Pub/Sub messages carry no ID, headers, or attribute bag, and the payload itself is not included.

```json
{
  "redis_message": {
    "channel": "orders",
    "payload_size": 52
  }
}
```

For a pattern subscription (`PSUBSCRIBE`), `channel` is the concrete channel the message arrived on — not the pattern. So an app subscribed to `orders.*` that receives a message on `orders.created` reports:

```json
{
  "redis_message": {
    "channel": "orders.created",
    "payload_size": 52
  }
}
```

* <a id="lagged"></a>**`lagged`** — your consumer fell behind and the operator dropped `count` events:

```json
{
  "lagged": {
    "count": 12
  }
}
```

Lagging takes place whenever the consumer is not able to keep up with the messages and receive them in a timely fashion (e.g. due to a slow network connection). By default, the operator buffers up to 2048 messages (configurable in `values.yaml` through `subscribeEventBufferSize`), and lagging will take place if more than this many messages accumulate in the internal buffer without the consumer receiving them. Note that lagging only affects slow consumers — functioning consumers will continue to receive all events even in the presence of slow peers.

### Direct Kube API access (no mirrord CLI required)

`mirrord subscribe` is a thin wrapper over an operator endpoint that emits [Server-Sent Events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events) (`text/event-stream`). You can access it directly through the Kubernetes API, without requiring the mirrord CLI:

```sh
kubectl get --raw \
  "/apis/operator.metalbear.co/v1/events?watch=true&session_key=my-key"
```

The `subscribe` arguments `--session-key-field` and `--unmatched` correspond to the query parameters `include_session_key=true` and `include_unmatched=true`.

Omit `session_key` to receive every session's events on one stream. Such a stream always names the session each event belongs to, regardless of the value of `include_session_key`. `mirrord subscribe` always includes a key; to see events from all sessions, you can also use the [local UI](local-ui.md).

or with `kubectl proxy` + `curl`:

```sh
kubectl proxy &
curl -N "http://127.0.0.1:8001/apis/operator.metalbear.co/v1/events?watch=true&session_key=my-key"
```

`watch=true` is necessary to stop the Kube API server from cutting out the stream after \~60s.
