# Everything that can travel from one hop to the next

A checklist of what shows up in dumps of real microservice clusters, grouped by what puts it there. Use it to recognise what you see; `identifier-catalog.md` says which of these make a good mirrord filter. "Survives a hop" is the usual case, not a rule: the dump decides.

Legend: **Filterable** = mirrord can match it (`header_filter`, `header_filter_jq`, `path_filter`, `body_filter`). **Survives a hop** = services tend to forward it without extra work (✅), only with tracing or explicit code (🔧), or almost never (❌).

## 1. Trace context (tracing SDKs and meshes forward these)

| Carrier | Format | Trace-id part | Set by | Survives |
|---------|--------|---------------|--------|----------|
| `traceparent` / `tracestate` | W3C: `00-<32 hex>-<16 hex>-<flags>` / `vendor=value,...` | trace-id | OTel, Datadog, Istio/Envoy, .NET, most SDKs | 🔧 any W3C tracer |
| `baggage` | W3C: `key=value,key2=value2` | member value | OTel baggage propagator, Datadog (default on recent tracers), Spring `remote-fields` | 🔧 needs baggage enabled |
| `x-b3-traceid`, `x-b3-spanid`, `x-b3-parentspanid`, `x-b3-sampled`, `x-b3-flags`, `b3` | Zipkin B3 multi / single (`<trace>-<span>-<sampled>`) | `x-b3-traceid`; first segment of `b3` | Zipkin, Brave, Istio when configured for Zipkin, Linkerd | 🔧 |
| `x-request-id` | opaque, usually UUID | whole | Envoy/Istio, NGINX ingress, Kong, Linkerd; apps must forward (Istio lists it with `traceparent`/`tracestate` and the B3 set) | 🔧 |
| `x-datadog-trace-id`, `x-datadog-parent-id`, `x-datadog-sampling-priority`, `x-datadog-origin`, `x-datadog-tags` | decimal ids; `x-datadog-tags` is `_dd.p.tid=<hex>,...` | `x-datadog-trace-id` (lower 64 bits) | Datadog tracers | 🔧 |
| `x-cloud-trace-context` | `<32 hex>/<span>;o=1` | before `/` | Google front ends, GKE ingress, Cloud Run, Google SDKs | 🔧 |
| `x-amzn-trace-id` | `Root=1-<8 hex>-<24 hex>;Parent=<16 hex>;Sampled=1` | `Root=` value | ALB, API Gateway, X-Ray SDK | 🔧 ALB rewrites `Root` if it is not X-Ray shaped |
| `uber-trace-id` | `<trace>:<span>:<parent>:<flags>` | first segment | Jaeger clients (legacy) | 🔧 |
| `sw8`, `sw8-x`, `sw8-correlation` | SkyWalking v3: eight dash-separated base64 fields; correlation is `base64(key):base64(value),...` (max 3 keys, 128-byte values) | field 2 of `sw8`, base64 | SkyWalking agents | 🔧 |
| `sentry-trace` | `<32 hex>-<16 hex>-<0 or 1>` plus `sentry-*` members in `baggage` | first segment | Sentry SDKs | 🔧 |
| `elastic-apm-traceparent` | same as `traceparent` | trace-id | Elastic APM agents (legacy name) | 🔧 |
| `x-instana-t`, `x-instana-s`, `x-instana-l` | hex ids, level | `x-instana-t` | Instana | 🔧 |
| `newrelic` | base64 JSON payload | `d.tr` inside | New Relic agents (also use W3C) | 🔧 |
| `x-honeycomb-trace` | `1;trace_id=...,parent_id=...,context=<base64>` | `trace_id=` | Honeycomb beelines | 🔧 |
| `x-dynatrace`, `singularityheader` | vendor opaque | not parseable | Dynatrace, AppDynamics | 🔧 |
| `x-ot-span-context`, `ot-tracer-traceid`, `ot-tracer-spanid`, `ot-tracer-sampled` | OpenTracing / Lightstep legacy | `ot-tracer-traceid` | old Istio + Lightstep setups | 🔧 |
| `grpc-trace-bin` | base64 binary: version byte, then 16-byte trace id | bytes 2–17 | OpenCensus gRPC, some Go services | 🔧 gRPC only |
| `ce-traceparent`, `ce-tracestate` (binary mode) or `traceparent` / `tracestate` fields in the event JSON (structured mode) | CloudEvents distributed-tracing extension: carries the trace of the **starting point**, not of each hop | trace-id | Knative, event gateways | 🔧 body field in structured mode |

**Filterable:** all of these via `header_filter` on the trace-id part (regex in `filter-recipes.md`), binary/base64 ones via `header_filter_jq`.

## 2. Baggage-like key/value carriers (user-defined values that ride the trace)

These are the carriers a developer-chosen value can ride on without a new header. If any of them survives every hop, `mirrord-session=<key>` can live in it.

| Carrier | Format | Set by | Survives |
|---------|--------|--------|----------|
| `baggage` | W3C `key=value,...` | OTel, Datadog, Spring, any app | 🔧 |
| `tracestate` | `vendor=value,...` (32-entry cap; some SDKs rewrite it) | W3C tracers | 🔧 |
| `correlation-context` | pre-2020 name of W3C baggage | old .NET (`Activity.Baggage`), old OTel | 🔧 legacy |
| `uberctx-<key>` | one header per key | Jaeger clients; Signadot's browser extension uses `uberctx-sd-routing-key` | 🔧 |
| `jaeger-baggage` | `k=v, k2=v2` | Jaeger clients | 🔧 |
| `ot-baggage-<key>` | one header per key | OpenTracing / Lightstep | 🔧 |
| `baggage-<key>`, `baggage_<key>` | one header per key | Spring Cloud Sleuth (old), Brave `BaggagePropagation` | 🔧 |
| `sw8-correlation` | SkyWalking correlation context | SkyWalking | 🔧 |
| `x-datadog-tags` | `_dd.p.*` entries | Datadog | 🔧 tracer-owned, don't add keys |

**Filterable:** yes, by regex on the member with boundaries on both sides (`^uberctx-mirrord-session: {{ key }}$`, `^correlation-context:\s*(.*,\s*)?mirrord-session={{ key }}\s*([,;].*)?$`).

## 3. Correlation and request ids (one value per request)

| Carrier | Set by | Survives |
|---------|--------|----------|
| `x-request-id`, `x-correlation-id`, `correlation-id`, `x-transaction-id`, `request-id` | ingress, gateway, or first service; forwarded by convention | 🔧 |
| `request-id` + `request-context` | Azure Application Insights legacy hierarchical ids | 🔧 |
| `x-ms-request-id`, `x-ms-client-request-id`, `x-ms-correlation-request-id` | Azure SDKs and ARM | 🔧 |
| `cf-ray`, `x-vercel-id`, `fly-request-id`, `x-amz-cf-id`, `kong-request-id`, `x-kong-request-id`, `x-apigee-*` | CDN / edge / gateway request ids | ❌ edge only, not client-settable |
| `idempotency-key`, `x-idempotency-key` | API clients (Stripe-style) | ❌ edge only |
| `x-test-run-id`, `x-run-id`, `x-scenario`, `x-synthetic` | test harnesses, load generators, synthetics | 🔧 teams that propagate them do so deliberately |

**Filterable:** yes (`header_filter` exact match). Controllable only if the edge preserves the client's value; see the probe.

## 4. Identity (who the caller is)

| Carrier | Format | Set by | Survives |
|---------|--------|--------|----------|
| `authorization: Bearer <jwt>` | JWT claims `sub`, `email`, `preferred_username`, `tenant_id`, `org_id`, `groups`, `scope` | IdP via the client | 🔧 often exchanged for a service token or stripped internally |
| `authorization: Basic ...`, `x-api-key`, `api-key` | opaque | client | ❌ edge only |
| `cookie` (`session`, `sid`, `_oauth2_proxy`, `AWSELBAuthSessionCookie*`, `__session`, IdP cookies) | session id or JWT | app / IdP / ALB | ❌ edge only |
| `x-auth-request-user`, `x-auth-request-email`, `x-auth-request-groups`, `x-auth-request-preferred-username`, `x-auth-request-access-token`, `x-forwarded-user`, `x-forwarded-email`, `x-forwarded-groups`, `x-forwarded-access-token`, `x-forwarded-preferred-username` | plain values / token | oauth2-proxy, Authentik, Pomerium, Traefik forward-auth (must be allowlisted in `authResponseHeaders`) | 🔧 |
| `x-remote-user`, `x-webauth-user`, `remote-user`, `x-auth-username` | plain | nginx `auth_request`, SSO proxies | 🔧 |
| `x-goog-authenticated-user-email`, `x-goog-authenticated-user-id`, `x-goog-iap-jwt-assertion` | `accounts.google.com:<value>`; JWT | Google IAP | 🔧 |
| `x-amzn-oidc-identity`, `x-amzn-oidc-data`, `x-amzn-oidc-accesstoken` | `sub` plain; signed JWT with user claims; access token | AWS ALB OIDC auth | 🔧 |
| `cf-access-authenticated-user-email`, `cf-access-jwt-assertion` | plain; JWT | Cloudflare Access | 🔧 |
| `x-ms-client-principal`, `x-ms-client-principal-name`, `x-ms-client-principal-id` | base64 JSON; plain | Azure App Service / Container Apps Easy Auth | 🔧 |
| `x-user-id`, `x-user-email`, `x-tenant-id`, `x-account-id`, `x-org-id`, `x-customer-id`, `x-workspace-id` | plain | the gateway or first service after auth | 🔧 the most common internal convention |
| `x-forwarded-client-cert` (XFCC), SPIFFE ID in the mTLS cert | service identity | Envoy/Istio, Linkerd, SPIRE | identifies the **calling service**, not the developer: not usable |
| `x-forwarded-for`, `forwarded`, `x-real-ip`, source IP | client address | ingress | ❌ edge only; not an HTTP filter target |

**Filterable:** plain headers by regex; JWT and base64 JSON by `header_filter_jq` (same recipe as `authorization`, different header name). Unique per identity, so pair with a route (`all_of`) when stealing.

## 5. Routing, environment and feature context (what teams add on purpose)

These exist because someone already needed per-developer or per-experiment routing. They are the first thing to look for: if the cluster has one, mirrord can reuse it.

| Carrier | Used by | Survives |
|---------|--------|----------|
| `x-env`, `x-stage`, `x-version`, `x-canary`, `x-release`, `x-lane`, `x-sandbox`, `x-route-to`, `x-dev-branch` | header-based canary / lane routing in Istio `VirtualService`, Linkerd `HTTPRoute`, NGINX, Kong | 🔧 teams propagate them explicitly, usually via baggage or a tracing hook |
| `l5d-dst-override` | Linkerd per-request destination override | 🔧 |
| `sd-routing-key` (in `baggage`, `tracestate`, `uberctx-sd-routing-key`, or `?sd-routing-key=`) | Signadot sandboxes | 🔧 |
| `x-telepresence-id` (or a custom header) | Telepresence personal intercepts | 🔧 |
| `mirrord-session=<key>` in `baggage` / `tracestate` | mirrord filters | 🔧 |
| `mirrord-key` | the Operator stamps it on traffic it routes to a session, including every request in your own dump; it names the session, not the developer's request | marker only, never a candidate |
| `x-feature-flags`, `x-ff-*`, `x-experiment`, `x-variant`, `x-ab-test`, `x-launchdarkly-*` | feature-flag overrides and experiment assignment | 🔧 |
| `x-debug`, `x-trace-debug`, `x-envoy-force-trace`, `x-verbose` | debug switches | 🔧 |
| `x-locale`, `accept-language`, `x-timezone`, `x-currency`, `x-region`, `x-market` | localisation context | 🔧 not unique |
| `x-client-version`, `x-app-version`, `x-platform`, `x-device-id`, `x-session-id`, `x-install-id`, `user-agent` | mobile / web client context | ❌ edge only, rarely unique |
| `x-mock`, `x-stub`, `x-dry-run` | test doubles switches | 🔧 |

**Filterable:** yes, regex. A routing header the team already propagates is as good as `baggage`.

## 6. Request content (not headers)

| Carrier | Where | Filterable | Survives |
|---------|-------|------------|----------|
| Path segments `/tenants/{id}/...`, `/users/{id}`, `/v2/orgs/{slug}` | URL | `path_filter` | ❌ internal routes differ |
| Query parameters `?tenant=`, `?session=`, `?sd-routing-key=` | URL | `path_filter` (matched with the query) | ❌ |
| JSON body fields `tenant_id`, `user_id`, `metadata.session`, `context.trace_id`, GraphQL `variables`/`extensions`, JSON-RPC `id`, CloudEvents structured `traceparent` | body | `body_filter` (JSONPath + regex); only JSON, only non-`GET` | ❌ bodies are rebuilt per hop |
| gRPC metadata (`-bin` keys are base64 on the wire) | HTTP/2 headers | `header_filter` / jq | 🔧 same as HTTP headers |
| WebSocket / SSE | only the upgrade or initial request has headers | first request only | ❌ |
| Response headers and bodies (`set-cookie`, `x-request-id` echoed back) | response | ❌ mirrord matches requests only | — |

## 7. Non-HTTP hops (where `mirrord dump` can't see)

| Hop | Where the value lives | mirrord filter |
|-----|----------------------|----------------|
| Kafka | record headers (`baggage`, `traceparent`, `b3`, `x-request-id`, `sw8`) | `split_queues` `message_filter` |
| RabbitMQ / AMQP | message `headers` table | `message_filter` |
| SQS / SNS | message attributes (`baggage`, `AWSTraceHeader`, Datadog `_datadog`) | `message_filter` or `jq_filter` on `.MessageAttributes` |
| GCP Pub/Sub | attributes (`baggage`, `googclient_*` from the client's OTel support, `ce-traceparent`) | `message_filter` / `jq_filter` |
| Azure Service Bus | application properties (`Diagnostic-Id` = W3C traceparent) | `message_filter` / `jq_filter` |
| NATS | headers | `message_filter` |
| Redis Pub/Sub, BullMQ, Celery (Redis) | no metadata: a field in the JSON payload | top-level field / `jq_filter` |
| Temporal | workflow/activity headers (`baggage`, `_tracer-data`) | `header.<name>` in `message_filter` |
| CloudEvents over any binding | `traceparent` / `tracestate` extension attributes | depends on binding |
| S3 / GCS object → bucket notification | object user metadata (`x-amz-meta-baggage`, `x-goog-meta-baggage`); the notification itself carries no request context | `jq_filter` on `.S3Metadata` / `.gcsMetadata` |
| Outbox table / CDC (Debezium) | an outbox column emitted as a message header | `message_filter` |
| Cron, scheduled jobs, third-party webhooks, direct third-party uploads | nothing: no inbound request to carry a value | not filterable; list as not covered |

Inspect these with the broker's tools (`kcat -f '%h'`, `aws sqs receive-message --message-attribute-names All`, a test subscription) and look for the same identifier you chose for HTTP. The `mirrord-kafka` skill writes the queue filter; `mirrord-header-propagation` makes producers and consumers carry `baggage`.

## Things that look like carriers but are not

- **Service mesh sidecars** forward nothing for the app between an inbound and an outbound request. They only add their own headers (`x-request-id`, `x-b3-*`, `x-envoy-*`) when absent.
- **mTLS identity / SPIFFE** names the workload, so every developer's traffic through `orders` carries the same identity.
- **Source IP and `x-forwarded-for`** stop at the first hop and are not HTTP filter targets.
- **Connection reuse** (HTTP/2, gRPC channels, keep-alive) carries no per-request identity; only headers on each request do.
- **Server-side session state** (a session store keyed by cookie) is not on the wire between hops.
- **Response-only values** cannot be filtered on; a request id that only appears in a response has to be sent back in on the next request to count.
