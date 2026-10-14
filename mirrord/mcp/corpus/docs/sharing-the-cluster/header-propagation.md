---
title: Header Propagation
description: Keep your mirrord filter header on requests and messages as they pass between services, so filtered traffic reaches services deeper in the call chain
date: 2026-10-06T00:00:00.000Z
lastmod: 2026-10-11T00:00:00.000Z
draft: false
menu:
  docs:
    parent: sharing-the-cluster
toc: true
tags:
  - oss
  - team
  - enterprise
---

To share a cluster, each developer marks their traffic with a header, and mirrord sends each session only the traffic that carries its value. Two features work this way:

* [HTTP filters](../using-mirrord/incoming-traffic/filter-incoming-traffic.md) match a header on the requests that reach the target.
* [Queue splitting](queue-splitting.md) filters match a header or attribute on the messages the target consumes.

mirrord only sees the request or message that reaches the target. If another service created that request or message, the header is there only if that service copied it over. This is called header propagation, and it is up to your application, not mirrord.

{% hint style="info" %}
The [`mirrord-header-propagation`](https://github.com/metalbear-co/skills/tree/main/skills/mirrord-header-propagation) skill lets your AI agent set up header propagation for you. See [Use `baggage` and your tracing library](#use-baggage-and-your-tracing-library).
{% endhint %}

## When you need header propagation

Consider a request that passes through `frontend` to `billing-api`, where you run `billing-api` with mirrord:

![A client sends a request with the header baggage: mirrord-session=alice to frontend, which calls billing-api. Without propagation, frontend drops the header, the filter on billing-api doesn't match, and the deployed container handles the request. With propagation, frontend forwards the header, the filter matches, and the request reaches billing-api on your machine.](../.gitbook/assets/header-propagation.svg)

The client adds the header to its request to `frontend`. `frontend` then makes a *new* HTTP call to `billing-api`. Unless `frontend` copies the header from the request it received onto the request it sends, the header is gone, `billing-api` receives an ordinary request, and your filter doesn't match.

Messages work the same way. If `frontend` publishes a message to a queue instead of calling `billing-api`, the message has the header only if `frontend` copies it from the request. Without it, your queue splitting filter doesn't match, and the deployed `billing-api` consumes the message.

You need header propagation when you filter by a header, and the request or message that reaches your target is made by another service while it handles your traffic.

You don't need it when:

* You steal or mirror unfiltered traffic.
* You filter by something other than a header, such as an HTTP path or a message body.
* You send requests directly to the service you run with mirrord.
* You publish the messages yourself, with the header already on them.

## Recognizing the problem

Missing propagation looks like mirrord isn't working, even though the session is healthy:

* `mirrord exec` reports that it's ready, and `mirrord operator status` shows your session with the expected filter.
* Requests or messages you send with the header are handled by the deployed target instead of reaching you.
* A request that you send with the header **directly** to the service you run with mirrord does reach you.

To confirm for HTTP, run [`mirrord dump`](../using-mirrord/incoming-traffic/inspect-live-traffic.md) against the service you run with mirrord and send a request through the normal entry point. If the request arrives without your header, a service earlier in the chain is dropping it. For queues, look at the headers or attributes of a message in your broker's console or CLI.

{% hint style="info" %}
`mirrord dump` shows the raw bytes the service receives. If the service terminates TLS itself, those bytes are encrypted and the headers aren't readable. In that case, check by temporarily logging the incoming request headers in the service instead.
{% endhint %}

## Use `baggage` and your tracing library

{% hint style="info" %}
**Let your AI agent set this up.** The [`mirrord-header-propagation`](https://github.com/metalbear-co/skills/tree/main/skills/mirrord-header-propagation) skill finds the HTTP, gRPC and queue flows in your services and enables `baggage` propagation for you. [Install the mirrord skills](../using-mirrord-with-ai/ai-skills-plugin.md), then ask your agent to *"propagate the mirrord session header across my services."* To set it up by hand, follow the steps below.
{% endhint %}

The simplest setup is to filter on the W3C `baggage` header and let OpenTelemetry carry it between services. The same regex works for an HTTP filter and for a queue splitting filter:

```json
{
  "feature": {
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "^baggage: .*mirrord-session=alice(?:[,; ]|$)"
        }
      }
    },
    "split_queues": {
      "billing-queue": {
        "queue_type": "Kafka",
        "filter": { "metadata": "^baggage: .*mirrord-session=alice(?:[,; ]|$)" }
      }
    }
  }
}
```

The `(?:[,; ]|$)` at the end makes the filter match only the complete value `alice`. Without it, a filter for `alice` would also match traffic meant for another session whose key starts with `alice`, such as `alice2`.

OpenTelemetry instrumentation extracts `baggage` from each incoming request and adds it to the outgoing requests made while handling it. If your services are already instrumented, they may propagate it with no further changes. The defaults in common setups:

| Stack | What to enable |
| --- | --- |
| Java | The OpenTelemetry Java agent propagates `tracecontext` and `baggage` by default. |
| Python | `opentelemetry-instrument` with the instrumentation packages for your web framework and HTTP client. `baggage` is in the default `OTEL_PROPAGATORS`. |
| Node.js | The OpenTelemetry Node SDK with `@opentelemetry/auto-instrumentations-node`. The default propagators include `baggage`. |
| Go | There's no default propagator in Go. Register one with `otel.SetTextMapPropagator(propagation.NewCompositeTextMapPropagator(propagation.TraceContext{}, propagation.Baggage{}))`, then wrap your server handlers with `otelhttp.NewHandler` and your HTTP clients with `otelhttp.NewTransport`. For gRPC, use the `otelgrpc` stats handlers. |

If you set `OTEL_PROPAGATORS` explicitly, make sure it includes `baggage`.

Some messaging instrumentation also adds the context to the messages a service publishes, and restores it in the consumer. Coverage differs by language and broker, so check that `baggage` is on a published message before you rely on it. If it isn't, copy it yourself, as described in the next section.

{% hint style="info" %}
Instrumentation propagates context only for calls made while handling the request. Work handed off to a background thread, job queue or scheduler usually loses the request context, and the header with it, unless you pass the context along explicitly.
{% endhint %}

## Propagating a custom header

If you filter on your own header (for example `x-dev-session: alice`), tracing libraries won't forward it. Each service has to read it from the incoming request and add it to every outgoing call and published message. The best place for this is shared middleware or a shared client library used by all your services, so that every service propagates the header without each team adding it themselves.

Example in Go with Gin, reading the header from the incoming request and forwarding it on outgoing HTTP and gRPC calls:

```go
session := c.GetHeader("x-dev-session")

// Outgoing HTTP request:
req.Header.Set("x-dev-session", session)

// Outgoing gRPC call: pass the returned context to the RPC.
ctx := metadata.AppendToOutgoingContext(c, "x-dev-session", session)
resp, err := billingClient.GetInvoice(ctx, invoiceReq)
```

For a message, put the header in the message's metadata: a Kafka or RabbitMQ header, an SQS or Pub/Sub message attribute, or an Azure Service Bus application property. A queue splitting filter matches against that metadata. See [Setting a Filter for a mirrord Run](queue-splitting.md#setting-a-filter-for-a-mirrord-run) for how each broker exposes it.

Prefer `baggage` if you can. Tracing libraries forward it with little or no extra code, and other mirrord features, including [preview environments](../use-cases/preview-environments.md) and the [browser extension](../using-mirrord/incoming-traffic/debug-from-browser.md), use it by default.

## Check your ingress

For HTTP, the header must also survive the edge of your cluster. Most ingress controllers and load balancers pass request headers through unchanged, but some API gateways, CDNs and WAFs remove headers they don't recognize. If the first service receives the request without your header (checked with `mirrord dump`, or with header logging if the service terminates TLS), check the configuration of whatever sits in front of it.
