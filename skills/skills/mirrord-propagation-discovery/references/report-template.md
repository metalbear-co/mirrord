# Report template

The skill ends with this report, printed in the conversation and saved to `.mirrord/propagation-discovery.md` next to the generated config (or the path the user asks for). The example below is filled in for a fictional four-service flow so the shape is clear; replace every value with what the dumps showed. Header **names** and value **shapes** only: never a token, cookie, or captured trace id.

---

# Propagation discovery — `staging`, `POST /v1/orders`

**Filter on:** the `mirrord-session` member of the `baggage` header.
**Why:** it reached 3 of 4 services with the value sent at the edge, the developer sets it per request (`curl -H`, browser extension), and the OpenTelemetry Node and Java agents on those services already forward it. No code change is needed for the covered services.

## Services covered (3 of 4)

| Service | Port | How the identifier arrives | Dump evidence |
|---------|------|----------------------------|---------------|
| `deployment/gateway` | 8080 | set by the client | probe value seen in `baggage` on 1/1 probe request; 100% of 212 observed requests carry `baggage` |
| `deployment/orders` | 8080 | forwarded by OTel Node auto-instrumentation | probe value seen; 100% of 198 requests |
| `deployment/inventory` | 9090 (gRPC) | forwarded by OTel Java agent | probe value seen as gRPC metadata; 100% of 187 requests |

## Services not covered (1 of 4)

| Service | Port | What arrives instead | Why it is dropped | Fix (who) |
|---------|------|----------------------|-------------------|-----------|
| `deployment/billing` | 8080 | `traceparent` with a **new** trace id, no `baggage`, `authorization` is a service token | `inventory` calls billing through a hand-built `http.Client` without the OTel transport, so no context is injected | wrap that client with `otelhttp.NewTransport` (app team, `mirrord-header-propagation` skill) |

## Not observable with `mirrord dump`

| Hop | Reason | How to check |
|-----|--------|--------------|
| `deployment/notifications-worker` | consumes SQS `order-events`; inbound is an outgoing connection to SQS | `aws sqs receive-message --message-attribute-names All` on a test message; filter with `split_queues` `message_filter` on `baggage` (`mirrord-kafka` skill pattern) |
| `deployment/payments` | terminates TLS in the pod (dump shows `TLS` connections, hex payloads) | Operator HTTPS stealing setup, then re-run the dump |

## Candidates considered

| Identifier | gateway | orders | inventory | billing | Unique | Controllable | Verdict |
|------------|---------|--------|-----------|---------|--------|--------------|---------|
| `baggage` mirrord-session | ✅ | ✅ | ✅ | ❌ | per request | yes | **chosen** |
| `tracestate` mirrord-session | ✅ | ✅ | ❌ rewritten by Java agent | ❌ | per request | yes | no |
| `traceparent` trace-id | ✅ | ✅ | ✅ | ❌ new trace | per request | yes (curl only) | runner-up, same gap |
| `x-request-id` | ✅ rewritten by NGINX ingress | ✅ | ✅ | ✅ | per request | **no** | not controllable |
| `authorization` jwt.sub | ✅ | ✅ | ✅ | ❌ service token | per user | yes (own user) | runner-up for browser flows |
| `x-tenant-id` | ❌ | ✅ set by orders | ✅ | ✅ | per tenant | yes (own tenant) | starts one hop too late |
| `cookie` session | ✅ | ❌ | ❌ | ❌ | per session | yes | edge only |

## mirrord.json

Saved to `.mirrord/mirrord.json`. Run with `mirrord exec -f .mirrord/mirrord.json --key <your-name> -- <command>` and send requests with `-H "baggage: mirrord-session=<your-name>"`. Mirror mode first; switch `"mode"` to `"steal"` once only your requests arrive.

```json mirrord
{
  "target": {
    "path": "deployment/orders",
    "namespace": "staging"
  },
  "feature": {
    "network": {
      "incoming": {
        "mode": "mirror",
        "ports": [8080],
        "http_filter": {
          "header_filter": "^baggage:\\s*(.*,\\s*)?mirrord-session={{ key }}\\s*([,;].*)?$",
          "ports": [8080]
        }
      }
    }
  }
}
```

Change `target.path` and both `ports` for `gateway` (8080) or `inventory` (9090); the filter is the same.

## Next steps

1. `inventory → billing`: add the OTel transport to the billing client so `baggage` reaches billing (`mirrord-header-propagation`).
2. `notifications-worker`: confirm `baggage` is on the SQS message attributes; if not, the same propagation work covers the producer in `orders`.
3. `payments`: set up HTTPS stealing, then re-run this discovery for that hop.

**Verified:** mirror session on `deployment/inventory` with the config above and `scripts/request_recorder.py` as the local process received the tagged probe and not the untagged one. `billing` not verified (identifier does not reach it). `inventory` runs 2 replicas; both were dumped.
**Dumps:** 3-minute window on each service plus 2 active probes, 2026-10-13; logs deleted.
