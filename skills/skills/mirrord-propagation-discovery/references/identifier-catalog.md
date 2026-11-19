# Identifier catalog

What to look for in dump output, ranked. A candidate is usable only when it has all three properties on every hop in scope:

| Property | Meaning | How to check |
|----------|---------|--------------|
| **Present** | The value reaches every hop, on every request in the flow | Presence per hop ≈ 100% in the dump stats; the probe value shows up in every hop's dump |
| **Unique** | It separates one developer's traffic from everyone else's. Either unique per request (a correlation id the developer mints) or unique per identity (a claim or tenant only that developer uses) | Distinct-value ratio in the stats: ~1.0 means per request; a stable value per caller means per identity |
| **Controllable** | The developer can put their own value on the request at the edge, or own the identity the value refers to | Can be set with `curl -H`, the mirrord browser extension, or a test account the developer owns |

Identifiers that are unique but not controllable (an ingress-minted request id the client cannot override) only work for one-off inspection, not for routing a developer's session.

`carriers.md` is the long list of everything that can travel between hops (vendor trace headers, legacy baggage names, auth-proxy identity headers, routing and feature-flag headers, queue and event carriers). This file keeps only the shapes worth filtering on.

## Candidates, best first

### 1. `baggage` (W3C)

- **Who sets it:** the caller. Members are `key=value` pairs separated by commas; mirrord's convention is `mirrord-session=<key>`.
- **Propagated by:** OpenTelemetry (baggage propagator enabled), Datadog on recent tracers, Spring/Micrometer with `remote-fields`, or an explicit propagation layer. See the `mirrord-header-propagation` skill.
- **Filter:** `"header_filter": "^baggage:\s*(.*,\s*)?mirrord-session={{ key }}\s*([,;].*)?$"` (member boundaries on both sides, see `filter-recipes.md`)
- **Pitfalls:** only reaches hops whose service forwards it. Gateways with header allowlists drop it. Sentry and other SDKs add their own members, so match the member, never the whole value.

### 2. `tracestate` (W3C)

- **Who sets it:** the caller, as `vendor=value` entries.
- **Propagated by:** the tracecontext propagator. Most SDKs forward unknown entries unchanged.
- **Filter:** `"header_filter": "^tracestate:\s*(.*,\s*)?mirrord-session={{ key }}\s*(,.*)?$"`
- **Pitfalls:** the spec caps it at 32 entries and some SDKs truncate or rewrite it. Some proxies only forward it together with a valid `traceparent`, so send both.

### 3. Correlation-id headers: `x-request-id`, `x-correlation-id`, `request-id`, `x-amzn-trace-id`, `x-cloud-trace-context`, `x-b3-traceid`

- **Who sets it:** often the ingress or mesh (NGINX ingress, Envoy/Istio, ALB, Google front ends) when the client sends none. Apps must forward it by hand; Istio documents which headers an app has to copy.
- **Unique per:** request.
- **Filter:** `"header_filter": "^x-request-id: {{ key }}$"`
- **Pitfalls:** the ingress may overwrite a client-supplied value. The probe tells you: if the value at hop 1 differs from what you sent, it is not controllable from outside. Load balancers may inject their own id on a different header than the one the apps forward.

### 4. JWT claims in `authorization: Bearer <jwt>` (or a cookie holding a JWT)

- **Who sets it:** the identity provider. Claims such as `sub`, `email`, `preferred_username`, `tenant_id`, `org_id`.
- **Unique per:** identity. Controllable when the developer has their own user or tenant.
- **Filter:** jq on the header, see `filter-recipes.md`. Match a claim, never the raw token: tokens rotate and must not be written into config.
- **Pitfalls:** gateways often exchange the user token for a service token or strip `authorization` on internal calls, so the claim exists at the edge but not downstream. Opaque (non-JWT) tokens carry no claims. Encrypted JWTs (JWE, five dot-separated parts) cannot be decoded by the filter.

### 5. Identity headers set after auth: `x-user-id`, `x-tenant-id`, `x-account-id`, `x-org-id`

- **Who sets it:** the gateway or the first service after authentication.
- **Unique per:** identity.
- **Filter:** `"header_filter": "^x-tenant-id: {{ key }}$"`
- **Pitfalls:** only present downstream of the component that sets it. Often absent on service-to-service calls that use a system identity.

### 6. `traceparent` trace-id

- **Who sets it:** the caller, or the first instrumented component. Format `00-<trace-id 32 hex>-<span-id 16 hex>-<flags>`.
- **Propagated by:** every W3C-aware tracer; the trace-id stays constant, the span-id changes at each hop, so only ever match the trace-id.
- **Filter:** `"header_filter": "^traceparent: 00-{{ key }}-"` with a 32-hex-char key.
- **Pitfalls:** the developer has to mint a trace-id per request, so this suits scripted or `curl` traffic, not a browser session, unless the browser extension sets it. Some SDKs start a fresh trace when the incoming `traceparent` is unsampled (`-00`), so send `-01`.

#### Other trace-id carriers

Clusters that predate W3C tracecontext carry the same trace id under a vendor header. The dump shows which one the tracer uses; the trace id is the part the developer controls and the filter anchors on:

| Header | Format | Trace-id part | Filter |
|--------|--------|---------------|--------|
| `x-b3-traceid` (Zipkin/B3 multi) | `<16 or 32 hex>` | whole value | `^x-b3-traceid: {{ key }}$` |
| `b3` (B3 single) | `<trace>-<span>-<sampled>` | first segment | `^b3: {{ key }}-` |
| `x-cloud-trace-context` (Google) | `<32 hex>/<span>;o=1` | before `/` | `^x-cloud-trace-context: {{ key }}/` |
| `x-amzn-trace-id` (AWS X-Ray) | `Root=1-<8 hex>-<24 hex>;Parent=...;Sampled=1` | `Root=` value | `^x-amzn-trace-id: .*Root={{ key }}(;\|$)` |
| `uber-trace-id` (Jaeger) | `<trace>:<span>:<parent>:<flags>` | first segment | `^uber-trace-id: {{ key }}:` |

The same pitfalls as `traceparent` apply: the span part changes per hop, a service that starts a new trace breaks the chain downstream, and ALB / API Gateway may overwrite `x-amzn-trace-id` with their own `Root`. `dump_stats.py` extracts the trace-id part of each of these as `<header>:trace-id` so you can compare it across hops directly.

### 7. Cookies

- **Who sets it:** the app or the identity provider at login.
- **Unique per:** session or identity.
- **Filter:** `"header_filter": "^cookie:\s*(.*;\s*)?session_id={{ key }}\s*(;.*)?$"`, or jq when the cookie holds a JWT.
- **Pitfalls:** cookies rarely survive past the first service. Treat as edge-only unless the dump proves otherwise.

### 8. Path segments and query parameters

- **Examples:** `/tenants/{id}/...`, `/users/{id}/...`, `?session={key}`.
- **Filter:** `"path_filter": "^/tenants/{{ key }}/"` (the path filter is matched against the path and the query string).
- **Pitfalls:** internal calls usually use different routes, so the segment exists on the edge route only. Unique per tenant, not per request.

### 9. JSON body fields

- **Examples:** `user_id`, `tenant`, `metadata.session`.
- **Filter:** `body_filter` with a JSONPath query and a regex on the result, see `filter-recipes.md`.
- **Pitfalls:** only on requests with a JSON body (never `GET`). The agent must buffer each body to evaluate it. Every hop must carry the field in its own body, which is rare on fan-out calls.

## Hops `mirrord dump` cannot show

- **Queue consumers:** the consumer's input arrives over a connection the pod opened to the broker, so it is outgoing traffic and does not show in a dump of the consumer. Inspect messages directly (Kafka `kcat -C -f '%h\n'`, `aws sqs receive-message --message-attribute-names All`, RabbitMQ management "Get messages") and check for the same identifier as a message header or attribute. The `mirrord-kafka` and `mirrord-header-propagation` skills cover filtering and propagation there.
- **TLS terminated by the target itself:** the dump shows `TLS` connections with hex payloads and no headers. Filtering those needs the Operator's HTTPS stealing setup. Traffic decrypted by a service mesh sidecar shows as plain HTTP.
- **Non-HTTP protocols:** database, Redis, and custom TCP traffic show as raw bytes. mirrord HTTP filters do not apply to them.
- **Responses:** mirrord filters match request headers, path, method and JSON body only. A value that exists only in a response (a `set-cookie`, a response id) cannot be filtered on.
